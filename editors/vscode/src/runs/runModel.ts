import type { Event, GetRunToolCallsResponse, LineageNode } from '../client';
import type { PlanStep, RunAttempt, RunView, ToolCallRow } from '../shared/protocol';
import { parseGatewayTime, toIsoUtc } from '../shared/time';
import { cleanBlock, cleanText, stripUnsafe } from '../shared/untrustedText';

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

// Run events are written by whatever the runner ran: every string is cleaned and bounded here, once,
// so no view downstream (tree, panel, thread) has to remember to.
function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' ? cleanText(value, 400) : undefined;
}

/** Free text that may legitimately span lines (an error, a summary). */
function blockValue(value: unknown): string | undefined {
  return typeof value === 'string' ? cleanBlock(value, 2000) : undefined;
}

function numberValue(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function planSteps(value: unknown): PlanStep[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const allowed = new Set<PlanStep['status']>(['pending', 'in_progress', 'completed', 'blocked']);
  const steps = value.flatMap((entry: unknown): PlanStep[] => {
    if (entry === null || typeof entry !== 'object') return [];
    const row = entry as Record<string, unknown>;
    if (typeof row.step !== 'string' || !allowed.has(row.status as PlanStep['status'])) {
      return [];
    }
    return [{ step: cleanText(row.step, 300), status: row.status as PlanStep['status'] }];
  });
  return steps.length ? steps : undefined;
}

/**
 * The lineage node owns summary attributes; final rows fill gaps and settle the status.
 */
export function buildRunView(runNode: LineageNode | undefined, runEvents: Event[]): RunView {
  let progressPlan: PlanStep[] | undefined;
  let progressTime = Number.NEGATIVE_INFINITY;
  let progressPosition = -1;
  let finished: Body = {};
  const attempts: RunAttempt[] = [];

  for (const [position, event] of runEvents.entries()) {
    if (event.label_skill !== 'escurel:run') continue;
    const body = bodyOf(event);
    if (event.title === 'run-attempt') {
      const n = numberValue(body.attempt);
      if (n !== undefined) {
        attempts.push({
          n,
          startedAt: toIsoUtc(body.started_at),
          endedAt: toIsoUtc(body.ended_at),
          outcome:
            typeof body.outcome === 'string' && body.outcome.length > 0
              ? stripUnsafe(body.outcome, 80)
              : 'unknown',
          error: blockValue(body.error),
        });
      }
    } else if (event.title === 'run-progress') {
      const plan = planSteps(body.plan);
      const time = parseGatewayTime(event.at)?.getTime() ?? Number.NEGATIVE_INFINITY;
      if (plan && (time > progressTime || (time === progressTime && position > progressPosition))) {
        progressPlan = plan;
        progressTime = time;
        progressPosition = position;
      }
    } else if (event.title === 'run-finished') {
      finished = body;
    }
  }

  attempts.sort((a, b) => a.n - b.n);
  const status = (stringValue(finished.status) ?? stringValue(runNode?.state) ?? 'running')
    .trim()
    .toLowerCase();
  const failure = ['failed', 'dead_letter'].includes(status)
    ? [
        ...new Set(
          [stringValue(finished.reason), stringValue(finished.error), attempts.at(-1)?.error]
            .map((x) => x?.trim() ?? '')
            .filter(Boolean),
        ),
      ].join(' — ') || undefined
    : undefined;
  return {
    runId:
      runNode?.id ??
      runEvents.find((event) => event.label_skill === 'escurel:run' && event.run_id !== null)
        ?.run_id ??
      '',
    status,
    tone: ['failed', 'dead_letter', 'cancelled'].includes(status)
      ? 'failed'
      : ['running', 'processed', 'planned'].includes(status)
        ? 'run'
        : 'neutral',
    harness: stringValue(runNode?.harness) ?? stringValue(finished.harness),
    model: stringValue(runNode?.model) ?? stringValue(finished.model),
    autonomy: stringValue(runNode?.autonomy) ?? stringValue(finished.autonomy),
    targetPageId: stringValue(runNode?.target_page_id) ?? stringValue(finished.target_page_id),
    traceId: stringValue(runNode?.trace_id) ?? stringValue(finished.trace_id),
    producedPageId: stringValue(finished.produced_instance),
    startedAt: toIsoUtc(runNode?.started_at) ?? toIsoUtc(finished.started_at),
    finishedAt: toIsoUtc(runNode?.finished_at) ?? toIsoUtc(finished.finished_at),
    depth: numberValue(runNode?.depth) ?? numberValue(finished.depth),
    attempts,
    maxAttempts: numberValue(runNode?.max_attempts) ?? numberValue(finished.max_attempts),
    plan: planSteps(finished.plan) ?? progressPlan ?? planSteps(runNode?.plan) ?? [],
    summary: blockValue(runNode?.summary) ?? blockValue(finished.summary),
    ...(failure ? { failure } : {}),
    toolCallCount: numberValue(runNode?.tool_calls) ?? numberValue(finished.tool_calls),
    calls: [],
    nextAfter: null,
  };
}

/** The gateway caps a summary at 2 KB; allow for the characters cleaning may add or keep. */
const SUMMARY_MAX = 2200;

function summaryFields(
  call: GetRunToolCallsResponse['calls'][number],
): Pick<ToolCallRow, 'argsSummary' | 'resultSummary'> {
  const clean = (v: unknown): string | undefined =>
    typeof v === 'string' && v !== '' ? cleanBlock(v, SUMMARY_MAX) : undefined;
  const argsSummary = clean(call.args_summary);
  const resultSummary = clean(call.result_summary);
  return {
    ...(argsSummary !== undefined ? { argsSummary } : {}),
    ...(resultSummary !== undefined ? { resultSummary } : {}),
  };
}

function callRow(call: GetRunToolCallsResponse['calls'][number]): ToolCallRow {
  return {
    seq: call.seq,
    tool: cleanText(String(call.tool ?? ''), 120),
    status: cleanText(String(call.status ?? ''), 60),
    errorCode: call.error_code == null ? call.error_code : cleanText(String(call.error_code), 80),
    durationMs: numberValue(call.duration_ms) ?? 0,
    bytes: {
      request: numberValue(call.request_bytes) ?? 0,
      response: numberValue(call.response_bytes) ?? 0,
    },
    at: toIsoUtc(call.at) ?? call.at,
    ...summaryFields(call),
  };
}

/**
 * Replayed pages can overlap, so seq identifies a row across every page.
 *
 * Pages are fetched from the start in order. An empty final page ends paging. For
 * non-empty pages, only the one that reaches furthest decides `nextAfter`: a late
 * page 1 refresh must not restore its stale cursor after the last page.
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
    nextAfter:
      page.calls.length === 0 && page.next_after === null
        ? null
        : pageReach >= furthest
          ? page.next_after
          : view.nextAfter,
  };
}
